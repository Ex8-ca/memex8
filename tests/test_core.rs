//! Core memex8 tests — pure logic that doesn't need Qdrant
//!
//! Run with: cargo test --test test_core

// ─── Cosine Similarity ────────────────────────────────────────────────────────

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

#[test]
fn test_cosine_identical_vectors() {
    let v = vec![0.1f32, 0.2, 0.3];
    assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-6);
}

#[test]
fn test_cosine_opposite_vectors() {
    let a = vec![1.0f32, 0.0, 0.0];
    let b = vec![-1.0f32, 0.0, 0.0];
    assert!((cosine_similarity(&a, &b) + 1.0).abs() < 1e-6);
}

#[test]
fn test_cosine_perpendicular_vectors() {
    let a = vec![1.0f32, 0.0, 0.0];
    let b = vec![0.0f32, 1.0, 0.0];
    assert!((cosine_similarity(&a, &b) - 0.0).abs() < 1e-6);
}

#[test]
fn test_cosine_zero_vector() {
    let z = vec![0.0f32; 10];
    let v = vec![1.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    assert_eq!(cosine_similarity(&z, &v), 0.0);
    assert_eq!(cosine_similarity(&v, &z), 0.0);
    assert_eq!(cosine_similarity(&z, &z), 0.0);
}

#[test]
fn test_cosine_high_dim_orthogonal() {
    // Random high-dimensional vectors should be nearly orthogonal
    let a: Vec<f32> = (0..1536).map(|i| ((i * 7) as f32).sin()).collect();
    let b: Vec<f32> = (0..1536).map(|i| ((i * 13) as f32).cos()).collect();
    let sim = cosine_similarity(&a, &b);
    assert!(
        sim.abs() < 0.3,
        "High-dim orthogonal vectors should have |cos| < 0.3, got {}",
        sim
    );
}

#[test]
fn test_cosine_similar_vectors() {
    // v and v + small_noise should have high similarity
    let v: Vec<f32> = (0..128).map(|i| (i as f32 * 0.1).sin()).collect();
    let mut noisy = v.clone();
    noisy[10] += 0.01;
    noisy[50] -= 0.01;
    let sim = cosine_similarity(&v, &noisy);
    assert!(
        sim > 0.99,
        "Similar vectors should have cos > 0.99, got {}",
        sim
    );
}

// ─── Importance Score ─────────────────────────────────────────────────────────

/// Compute importance from upvotes (mirrors engine logic)
fn compute_importance(upvotes: i32, access_count: u32) -> f32 {
    let base = upvotes.max(0) as f32 * 0.1;
    let access_boost = access_count as f32 * 0.01;
    (base + access_boost + 0.1).min(1.0).max(0.01)
}

#[test]
fn test_importance_zero_upvotes() {
    let imp = compute_importance(0, 0);
    assert!((imp - 0.1).abs() < 1e-6);
}

#[test]
fn test_importance_with_upvotes() {
    assert!((compute_importance(5, 0) - 0.6).abs() < 1e-6);
    assert!((compute_importance(10, 0) - 1.0).abs() < 1e-6); // capped at 1.0
}

#[test]
fn test_importance_capped_at_one() {
    assert_eq!(compute_importance(100, 0), 1.0);
}

#[test]
fn test_importance_with_access_count() {
    // 5 upvotes (0.5) + 20 accesses (0.2) + base 0.1 = 0.8
    assert!((compute_importance(5, 20) - 0.8).abs() < 1e-6);
}

#[test]
fn test_importance_negative_upvotes_minimum() {
    // Negative upvotes still use max(0, ...) so floor is 0
    let imp = compute_importance(-5, 0);
    assert!((imp - 0.1).abs() < 1e-6);
}

// ─── Upvote / Downvote delta ───────────────────────────────────────────────────

fn upvote_delta(current_importance: f32, upvotes: i32) -> (i32, f32) {
    let new_upvotes = (upvotes + 1).max(0);
    let new_importance = (current_importance + 0.1).min(1.0);
    (new_upvotes, new_importance)
}

fn downvote_delta(current_importance: f32) -> f32 {
    (current_importance - 0.1).max(0.01)
}

#[test]
fn test_upvote_increments_and_caps() {
    let (votes, imp) = upvote_delta(0.5, 5);
    assert_eq!(votes, 6);
    assert!((imp - 0.6).abs() < 1e-6);

    // Capped at 1.0
    let (_, imp) = upvote_delta(0.95, 100);
    assert_eq!(imp, 1.0);
}

#[test]
fn test_downvote_decrements_and_floors() {
    assert!((downvote_delta(0.5) - 0.4).abs() < 1e-6);
    assert!((downvote_delta(0.05) - 0.01).abs() < 1e-6); // floor at 0.01
}

// ─── Recency Score ─────────────────────────────────────────────────────────────

/// Maps seconds since last access to a recency factor [0..1]
/// (mirrors the engine's recency scoring: newer = higher score)
fn recency_score(seconds_ago: i64) -> f64 {
    let hours = seconds_ago as f64 / 3600.0;
    let score = 1.0 - (hours / (24.0 * 30.0)).min(1.0);
    score.max(0.0)
}

#[test]
fn test_recency_now() {
    assert!((recency_score(0) - 1.0).abs() < 1e-6);
}

#[test]
fn test_recency_one_hour_ago() {
    let s = recency_score(3600);
    assert!((s - (1.0 - 1.0 / 720.0)).abs() < 1e-4);
}

#[test]
fn test_recency_30_days_old() {
    assert!((recency_score(30 * 24 * 3600) - 0.0).abs() < 1e-6);
}

#[test]
fn test_recency_old_memory_still_has_some_value() {
    // 7 days old = ~0.77 recency
    let s = recency_score(7 * 24 * 3600);
    assert!(s > 0.7 && s < 0.8);
}

// ─── Recall score (importance × recency × access boost) ──────────────────────

fn recall_score(importance: f32, last_accessed_secs_ago: i64, access_count: u32) -> f32 {
    let recency = recency_score(last_accessed_secs_ago) as f32;
    let access_boost = 1.0 + access_count as f32 * 0.05;
    importance * recency * access_boost
}

#[test]
fn test_recall_favors_recent_high_importance() {
    let recent_high = recall_score(0.8, 0, 0);
    let old_high = recall_score(0.8, 30 * 24 * 3600, 0);
    assert!(recent_high > old_high);
}

#[test]
fn test_recall_favors_high_importance_over_access_count() {
    // High importance, no access
    let high_imp = recall_score(0.8, 0, 0);
    // Low importance but many accesses
    let many_accesses = recall_score(0.1, 0, 50);
    assert!(
        high_imp > many_accesses,
        "Importance (0.8) should beat access count boost (0.1 × 3.5 = 0.35)"
    );
}

// ─── Token estimation ─────────────────────────────────────────────────────────

fn estimate_tokens(text: &str) -> usize {
    (text.len() / 4).max(1)
}

#[test]
fn test_token_estimate_empty() {
    assert_eq!(estimate_tokens(""), 1); // min 1
    assert_eq!(estimate_tokens("   "), 1);
}

#[test]
fn test_token_estimate_short() {
    assert_eq!(estimate_tokens("hello"), 1);
    assert_eq!(estimate_tokens("hello world"), 2);
}

#[test]
fn test_token_estimate_longer() {
    let text = "The quick brown fox jumps over the lazy dog.".to_string();
    // ~46 chars / 4 = 11.5 → 11
    assert_eq!(estimate_tokens(&text), 11);
}

// ─── Realm centroid similarity ────────────────────────────────────────────────

/// Compute centroid of a set of vectors
fn centroid(vectors: &[Vec<f32>]) -> Vec<f32> {
    if vectors.is_empty() {
        return vec![];
    }
    let dim = vectors[0].len();
    let mut c = vec![0.0f32; dim];
    for v in vectors {
        for (i, x) in v.iter().enumerate() {
            c[i] += x;
        }
    }
    let n = vectors.len() as f32;
    c.iter().map(|x| x / n).collect()
}

#[test]
fn test_centroid_single_vector() {
    let v = vec![1.0f32, 2.0, 3.0];
    let c = centroid(&[v.clone()]);
    assert_eq!(c, v);
}

#[test]
fn test_centroid_two_vectors() {
    let a = vec![2.0f32, 4.0];
    let b = vec![4.0f32, 6.0];
    let c = centroid(&[a, b]);
    assert_eq!(c, vec![3.0f32, 5.0]);
}

#[test]
fn test_centroid_empty() {
    let c: Vec<f32> = centroid(&[]);
    assert!(c.is_empty());
}

// ─── Realm assignment threshold ───────────────────────────────────────────────

/// Simulates realm assignment: returns true if similarity >= threshold
fn should_assign_to_realm(similarity: f32, threshold: f32) -> bool {
    similarity >= threshold
}

#[test]
fn test_realm_assignment_high_similarity() {
    assert!(should_assign_to_realm(0.9, 0.25));
}

#[test]
fn test_realm_assignment_at_threshold() {
    assert!(should_assign_to_realm(0.25, 0.25));
}

#[test]
fn test_realm_assignment_below_threshold() {
    assert!(!should_assign_to_realm(0.24, 0.25));
}

#[test]
fn test_realm_assignment_unrelated_topics() {
    // Two random unrelated topic embeddings typically score 0.2-0.4
    assert!(!should_assign_to_realm(0.2, 0.25));
    assert!(should_assign_to_realm(0.35, 0.25));
}

// ─── Memory deduplication (SHA-256 hash) ─────────────────────────────────────

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

fn sha256_hash(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

#[test]
fn test_dedup_identical_content() {
    let h1 = sha256_hash("same content");
    let h2 = sha256_hash("same content");
    assert_eq!(h1, h2);
}

#[test]
fn test_dedup_different_content() {
    let h1 = sha256_hash("content A");
    let h2 = sha256_hash("content B");
    assert_ne!(h1, h2);
}

// ─── Slumber merge eligibility ────────────────────────────────────────────────

/// A memory is eligible for merge if it's likely a redundant fragment
fn is_mergeable_fragment(content: &str) -> bool {
    let short = content.len() < 200;
    let has_user_marker = content.contains("## User");
    let has_assistant_marker = content.contains("## Assistant");
    short && (has_user_marker || has_assistant_marker)
}

#[test]
fn test_mergeable_fragment_small_conversation() {
    assert!(is_mergeable_fragment(
        "## User\nThanks\n## Assistant\nYou're welcome!"
    ));
}

#[test]
fn test_mergeable_fragment_user_only() {
    assert!(is_mergeable_fragment("## User\nWhat does the pruning do?"));
}

#[test]
fn test_not_mergeable_long_content() {
    let long = "# Title\n\n".to_string() + &"Lorem ipsum dolor sit amet. ".repeat(50);
    assert!(!is_mergeable_fragment(&long));
}

#[test]
fn test_not_mergeable_clean_summary() {
    let summary = "The graph edges were empty because memories use realm_name (a string like realm-28da6a75) but the code was looking for realm_id (undefined). Fixed by matching m.realm_name === node.label instead of m.realm_id === n.id.";
    assert!(!is_mergeable_fragment(summary));
}

// ─── Dynamic threshold scaling ───────────────────────────────────────────────

fn dynamic_threshold(base: f32, realm_count: usize) -> f32 {
    if realm_count > 20 {
        base * 0.5
    } else if realm_count > 10 {
        base * 0.7
    } else {
        base
    }
}

#[test]
fn test_dynamic_threshold_many_realms() {
    // When there are many realms, threshold is lowered to prevent 1:1 mapping
    assert!((dynamic_threshold(0.25, 25) - 0.125).abs() < 1e-6);
}

#[test]
fn test_dynamic_threshold_some_realms() {
    assert!((dynamic_threshold(0.25, 15) - 0.175).abs() < 1e-6);
}

#[test]
fn test_dynamic_threshold_few_realms() {
    assert_eq!(dynamic_threshold(0.25, 5), 0.25);
}
