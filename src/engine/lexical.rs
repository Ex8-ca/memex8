//! In-process BM25 keyword index over memory text.
//!
//! Vector search misses memories that share rare exact words with a query
//! (names, IDs, error strings). This index adds those as candidates. It is a
//! cache, not a source of truth: it is rebuilt from Qdrant at startup and after
//! slumber, and updated on direct writes. Every hit is re-scored against Qdrant
//! before use, so a stale entry can never surface a deleted memory.

use std::collections::HashMap;

const K1: f32 = 1.2;
const B: f32 = 0.75;
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "has", "have", "in", "is",
    "it", "its", "of", "on", "or", "that", "the", "this", "to", "was", "were", "will", "with",
];

/// Lowercase, split on anything that isn't a letter or digit, drop stopwords and
/// single letters (single digits are kept: "trip 2" should match "2").
pub fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .filter(|t| t.chars().count() > 1 || t.chars().all(|c| c.is_ascii_digit()))
        .filter(|t| !STOPWORDS.contains(t))
        .map(str::to_string)
        .collect()
}

#[derive(Default)]
pub struct LexicalIndex {
    /// term -> (memory id -> term frequency)
    postings: HashMap<String, HashMap<String, u32>>,
    /// memory id -> (distinct terms, token count)
    docs: HashMap<String, (Vec<String>, u32)>,
    total_len: u64,
}

impl LexicalIndex {
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// Index `text` under `id`, replacing any previous text for that id.
    pub fn upsert(&mut self, id: &str, text: &str) {
        self.remove(id);
        let tokens = tokenize(text);
        if tokens.is_empty() {
            return;
        }
        let mut tf: HashMap<String, u32> = HashMap::new();
        for t in &tokens {
            *tf.entry(t.clone()).or_default() += 1;
        }
        let len = tokens.len() as u32;
        for (term, n) in &tf {
            self.postings
                .entry(term.clone())
                .or_default()
                .insert(id.to_string(), *n);
        }
        self.total_len += len as u64;
        self.docs
            .insert(id.to_string(), (tf.into_keys().collect(), len));
    }

    pub fn remove(&mut self, id: &str) {
        if let Some((terms, len)) = self.docs.remove(id) {
            self.total_len -= len as u64;
            for term in terms {
                if let Some(p) = self.postings.get_mut(&term) {
                    p.remove(id);
                    if p.is_empty() {
                        self.postings.remove(&term);
                    }
                }
            }
        }
    }

    /// Top `k` (memory id, BM25 score), best first. Ties break on id so results
    /// are deterministic.
    pub fn search(&self, query: &str, k: usize) -> Vec<(String, f32)> {
        let n = self.docs.len() as f32;
        if n == 0.0 || k == 0 {
            return Vec::new();
        }
        let avg = self.total_len as f32 / n;
        let mut terms = tokenize(query);
        terms.sort();
        terms.dedup();
        let mut scores: HashMap<&str, f32> = HashMap::new();
        for term in &terms {
            let Some(p) = self.postings.get(term) else {
                continue;
            };
            let df = p.len() as f32;
            let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
            for (id, tf) in p {
                let len = self.docs.get(id).map_or(avg, |d| d.1 as f32);
                let tf = *tf as f32;
                *scores.entry(id.as_str()).or_default() +=
                    idf * tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * len / avg));
            }
        }
        let mut out: Vec<(String, f32)> = scores
            .into_iter()
            .map(|(id, s)| (id.to_string(), s))
            .collect();
        out.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        out.truncate(k);
        out
    }
}

/// Blend keyword hits into dense results.
///
/// Every candidate keeps its true cosine score and gains
/// `weight * bm25 / max_bm25` when it is a keyword hit. Dense candidates already
/// passed `min_score`; keyword-only candidates (scored by cosine via Qdrant)
/// must reach `min_score` after the bonus. An id present in both lists appears once.
pub fn apply_lexical_bonus(
    dense: Vec<(f32, crate::storage::qdrant::MemoryPoint)>,
    lexical_only: Vec<(f32, crate::storage::qdrant::MemoryPoint)>,
    hits: &[(String, f32)],
    weight: f32,
    min_score: f32,
) -> Vec<(f32, crate::storage::qdrant::MemoryPoint)> {
    let max = hits.iter().map(|h| h.1).fold(0.0_f32, f32::max);
    let bm25: HashMap<&str, f32> = hits.iter().map(|(id, s)| (id.as_str(), *s)).collect();
    let bonus = |id: &str| -> f32 {
        if max <= 0.0 {
            return 0.0;
        }
        bm25.get(id).map_or(0.0, |s| weight * s / max)
    };
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(dense.len() + lexical_only.len());
    for (s, p) in dense {
        if seen.insert(p.id.clone()) {
            let b = bonus(&p.id);
            out.push((s + b, p));
        }
    }
    for (s, p) in lexical_only {
        if seen.insert(p.id.clone()) {
            let total = s + bonus(&p.id);
            if total >= min_score {
                out.push((total, p));
            }
        }
    }
    out
}

/// Text indexed for a memory: heading, content and tags.
pub fn memory_text(m: &crate::storage::qdrant::MemoryPoint) -> String {
    let mut s = String::new();
    if let Some(h) = &m.heading {
        s.push_str(h);
        s.push(' ');
    }
    s.push_str(&m.content);
    for t in &m.tags {
        s.push(' ');
        s.push_str(t);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_lowercases_splits_and_drops_stopwords() {
        assert_eq!(
            tokenize("The Trip-Grus to Corby 2"),
            vec!["trip", "grus", "corby", "2"]
        );
    }

    #[test]
    fn rare_term_outranks_common_terms() {
        let mut idx = LexicalIndex::default();
        idx.upsert("a", "meeting notes about the project budget");
        idx.upsert("b", "Zephyr router flashed with OpenWrt firmware");
        idx.upsert("c", "project budget meeting again");
        let hits = idx.search("zephyr openwrt", 10);
        assert_eq!(hits.first().map(|h| h.0.as_str()), Some("b"));
    }

    #[test]
    fn upsert_replaces_and_remove_forgets() {
        let mut idx = LexicalIndex::default();
        idx.upsert("a", "alpha beta");
        idx.upsert("a", "gamma");
        assert!(idx.search("alpha", 5).is_empty());
        assert_eq!(idx.search("gamma", 5).len(), 1);
        idx.remove("a");
        assert!(idx.search("gamma", 5).is_empty());
        assert_eq!(idx.len(), 0);
        assert!(idx.is_empty());
    }

    #[test]
    fn empty_or_stopword_query_returns_nothing() {
        let mut idx = LexicalIndex::default();
        idx.upsert("a", "alpha");
        assert!(idx.search("", 5).is_empty());
        assert!(idx.search("the and of", 5).is_empty());
    }

    #[test]
    fn results_are_sorted_and_truncated() {
        let mut idx = LexicalIndex::default();
        for i in 0..20 {
            idx.upsert(&format!("m{i:02}"), &format!("token{} shared", i % 3));
        }
        let hits = idx.search("shared token1", 5);
        assert_eq!(hits.len(), 5);
        assert!(hits.windows(2).all(|w| w[0].1 >= w[1].1));
        // token1 docs carry an extra matching term, so they come first.
        assert!(hits[0].0 == "m01" || hits[0].0 == "m04");
    }

    fn mp(id: &str) -> crate::storage::qdrant::MemoryPoint {
        serde_json::from_value(serde_json::json!({
            "id": id, "content": "c", "summary": null,
            "source_file": null, "realm_id": "r", "realm_name": "R",
            "importance": 1.0, "upvotes": 0, "tags": [],
            "ingested_at": "2026-11-01T00:00:00Z", "last_accessed": "",
            "access_count": 0, "chunk_type": "manual", "heading": null,
            "source_hash": "h", "memory_type": "general", "visibility": "private"
        }))
        .unwrap()
    }

    #[test]
    fn bonus_without_hits_changes_nothing() {
        let out = apply_lexical_bonus(vec![(0.5, mp("a"))], vec![], &[], 0.15, 0.3);
        assert_eq!(out.len(), 1);
        assert!((out[0].0 - 0.5).abs() < 1e-6);
    }

    #[test]
    fn top_keyword_hit_gets_the_full_weight() {
        let hits = vec![("a".to_string(), 8.0), ("b".to_string(), 4.0)];
        let out = apply_lexical_bonus(
            vec![(0.5, mp("a")), (0.5, mp("b")), (0.5, mp("c"))],
            vec![],
            &hits,
            0.2,
            0.0,
        );
        let score = |id: &str| out.iter().find(|r| r.1.id == id).unwrap().0;
        assert!((score("a") - 0.7).abs() < 1e-6);
        assert!((score("b") - 0.6).abs() < 1e-6);
        assert!((score("c") - 0.5).abs() < 1e-6);
    }

    #[test]
    fn keyword_only_candidates_must_reach_min_score() {
        let hits = vec![("k1".to_string(), 10.0), ("k2".to_string(), 1.0)];
        let out = apply_lexical_bonus(
            vec![],
            vec![(0.2, mp("k1")), (0.2, mp("k2"))],
            &hits,
            0.15,
            0.3,
        );
        // k1: 0.2 + 0.15 = 0.35 kept; k2: 0.2 + 0.015 dropped.
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1.id, "k1");
    }

    #[test]
    fn ids_in_both_lists_appear_once() {
        let hits = vec![("a".to_string(), 1.0)];
        let out = apply_lexical_bonus(vec![(0.4, mp("a"))], vec![(0.4, mp("a"))], &hits, 0.1, 0.0);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn memory_text_includes_heading_content_and_tags() {
        let m: crate::storage::qdrant::MemoryPoint = serde_json::from_value(serde_json::json!({
            "id": "x", "content": "body text", "summary": null,
            "source_file": null, "realm_id": "r", "realm_name": "R",
            "importance": 1.0, "upvotes": 0, "tags": ["tagone"],
            "ingested_at": "2026-11-01T00:00:00Z", "last_accessed": "",
            "access_count": 0, "chunk_type": "manual", "heading": "Heading",
            "source_hash": "h", "memory_type": "general", "visibility": "private"
        }))
        .unwrap();
        let t = memory_text(&m);
        assert!(t.contains("Heading") && t.contains("body text") && t.contains("tagone"));
    }
}
